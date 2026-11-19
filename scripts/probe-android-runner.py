#!/usr/bin/env python3
"""No-download prerequisite evidence only. No VM, package install or inference."""
import os
from pathlib import Path
import platform
import stat
import subprocess
import sys


def kvm_status(path=Path('/dev/kvm')):
    try:
        if not stat.S_ISCHR(path.stat().st_mode): return 'not-character-device'
    except FileNotFoundError: return 'missing'
    except OSError: return 'stat-denied'
    try:
        fd=os.open(path,os.O_RDWR | os.O_CLOEXEC | os.O_NONBLOCK)
    except PermissionError: return 'open-denied'
    except OSError: return 'open-failed'
    else:
        os.close(fd)
        return 'open-readwrite-ok'


def executable(path):
    return path.is_file() and os.access(path,os.X_OK)


def collect(sdk):
    results={'host':'supported' if platform.system()=='Linux' and platform.machine()=='x86_64' else 'unsupported',
             'uid':'nonroot' if os.geteuid()!=0 else 'root-rejected',
             'kvm':kvm_status() if os.geteuid()!=0 else 'not-probed-root'}
    for label,relative in [('emulator','emulator/emulator'),('adb','platform-tools/adb'),
                           ('sdkmanager','cmdline-tools/latest/bin/sdkmanager'),('avdmanager','cmdline-tools/latest/bin/avdmanager')]:
        results[label]='present' if sdk is not None and executable(sdk/relative) else 'missing'
    results['emulator-version']='not-verified'
    if sdk is not None:
        try:
            lines=(sdk/'emulator/source.properties').read_text().splitlines()
            revisions=[line.split('=',1)[1].strip() for line in lines if line.startswith('Pkg.Revision') and '=' in line]
            results['emulator-version']='expected-37.2.13' if revisions==['37.2.13'] else 'other-or-ambiguous'
        except (OSError,UnicodeError): pass
    # Existence is evidence only, not approval of current/new license terms.
    results['license-file']='present-not-acceptance-proof' if sdk is not None and (sdk/'licenses/android-sdk-license').is_file() else 'missing'
    results['image']='not-verified'
    if sdk is not None:
        properties=sdk/'system-images/android-35/default/x86_64/source.properties'
        if properties.is_file():
            try:
                values={}
                for line in properties.read_text().splitlines():
                    if '=' in line:
                        key,value=line.split('=',1)
                        if key.strip() in values: raise ValueError()
                        values[key.strip()]=value.strip()
                expected={'Pkg.Revision':'2','AndroidVersion.ApiLevel':'35','SystemImage.Abi':'x86_64','SystemImage.TagId':'default'}
                results['image']='metadata-matches-not-payload-proof' if all(values.get(k)==v for k,v in expected.items()) else 'metadata-mismatch'
            except (OSError,UnicodeError,ValueError): results['image']='metadata-unreadable'
    results['acceleration']='not-probed'
    if results['host']=='supported' and results['uid']=='nonroot' and results['kvm']=='open-readwrite-ok' and results['emulator']=='present':
        try:
            completed=subprocess.run([str(sdk/'emulator/emulator'),'-accel-check'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=20)
            results['acceleration']='tool-success-not-vm-boot' if completed.returncode==0 else 'tool-rejected'
        except subprocess.TimeoutExpired: results['acceleration']='timeout'
        except OSError: results['acceleration']='launch-failed'
    return results


def main():
    expected=os.environ.get('MADINFER_EXPECTED_SHA','')
    if len(expected)!=40 or any(c not in '0123456789abcdef' for c in expected): raise ValueError()
    actual=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()
    if actual!=expected or subprocess.check_output(['git','status','--porcelain']): raise ValueError()
    raw=os.environ.get('ANDROID_HOME')
    sdk=Path(raw) if raw and Path(raw).is_absolute() else None
    results=collect(sdk)
    for label,value in results.items(): print('PREREQ '+label+'='+value)
    # Evidence collection success does not authorize downloading or claim readiness.
    print('PREREQ conclusion=observation-only-no-runtime-or-license-approval')

if __name__=='__main__':
    try: main()
    except Exception:
        print('FAIL: prerequisite-probe-preflight-or-collection',file=sys.stderr)
        sys.exit(1)
